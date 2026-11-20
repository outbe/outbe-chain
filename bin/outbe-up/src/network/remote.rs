use std::{
    path::{Path, PathBuf},
    process::Command,
};

use eyre::{bail, ensure, Context, Result};
use outbe_software_bundle::sha256_file;

use super::{
    build::{BuildState, NetworkManifest},
    config::{Founder, NetworkConfig},
};

pub(super) struct NetworkArtifacts {
    pub(super) root: PathBuf,
    pub(super) config: NetworkConfig,
    pub(super) manifest: NetworkManifest,
}

impl NetworkArtifacts {
    pub(super) fn load(root: &Path) -> Result<Self> {
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
            let expected = format!("dist/validator-{index}.tgz");
            ensure!(
                artifact.index == index
                    && artifact.name == founder.name
                    && artifact.ssh_host == founder.ssh.host
                    && artifact.advertise_ip == founder.advertise_ip
                    && artifact.archive == expected,
                "founder {index} does not match its network manifest"
            );
            ensure!(
                artifact.archive_sha256 == sha256_file(&root.join(&expected))?,
                "founder {index} archive changed after network build"
            );
        }
        Ok(Self {
            root: root.to_owned(),
            config,
            manifest,
        })
    }
}

#[derive(Clone)]
pub(super) struct SshOptions {
    ssh_key: Option<PathBuf>,
}

impl SshOptions {
    pub(super) fn new(ssh_key: Option<&Path>) -> Result<Self> {
        if let Some(path) = ssh_key {
            ensure!(path.is_file(), "SSH key does not exist: {}", path.display());
        }
        Ok(Self {
            ssh_key: ssh_key.map(Path::to_owned),
        })
    }

    pub(super) fn capture(&self, founder: &Founder, command: &str) -> Result<String> {
        let mut process = Command::new("ssh");
        self.ssh_args(&mut process, founder);
        let output = process.arg(command).output().wrap_err("run ssh")?;
        if !output.status.success() {
            bail!(
                "SSH command failed on {}: {}",
                founder.name,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    pub(super) fn run(&self, founder: &Founder, command: &str) -> Result<()> {
        let mut process = Command::new("ssh");
        self.ssh_args(&mut process, founder);
        let status = process.arg(command).status().wrap_err("run ssh")?;
        ensure!(status.success(), "SSH command failed on {}", founder.name);
        Ok(())
    }

    pub(super) fn run_quiet(&self, founder: &Founder, command: &str) -> Result<()> {
        let mut process = Command::new("ssh");
        self.ssh_args(&mut process, founder);
        let output = process.arg(command).output().wrap_err("run ssh")?;
        if !output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let details = [stdout.trim(), stderr.trim()]
                .into_iter()
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            bail!("SSH command failed on {}:\n{}", founder.name, details);
        }
        Ok(())
    }

    pub(super) fn copy_to(
        &self,
        founder: &Founder,
        sources: &[&Path],
        destination: &str,
        recursive: bool,
    ) -> Result<()> {
        let mut process = Command::new("scp");
        process.args(["-q", "-P", &founder.ssh.port.to_string()]);
        process.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]);
        if let Some(ssh_key) = &self.ssh_key {
            process.arg("-i").arg(ssh_key);
        }
        if recursive {
            process.arg("-r");
        }
        process.args(sources);
        process.arg(format!(
            "{}@{}:{destination}",
            founder.ssh.user,
            scp_host(&founder.ssh.host)
        ));
        let status = process.status().wrap_err("run scp")?;
        ensure!(status.success(), "SCP failed for {}", founder.name);
        Ok(())
    }

    fn ssh_args(&self, process: &mut Command, founder: &Founder) {
        process.args(["-p", &founder.ssh.port.to_string()]);
        process.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]);
        if let Some(ssh_key) = &self.ssh_key {
            process.arg("-i").arg(ssh_key);
        }
        process.arg(format!("{}@{}", founder.ssh.user, founder.ssh.host));
    }
}

fn scp_host(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    }
}
