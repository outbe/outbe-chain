use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
};

use eyre::{bail, ensure, Context, Result};
use outbe_genesis_cli::launch_bundle::quote;
use outbe_software_bundle::sha256_file;

use super::{
    build::{BuildState, FounderArtifact, NetworkManifest, MANIFEST_VERSION},
    config::NetworkConfig,
};

pub(super) struct NetworkArtifacts {
    pub(super) root: PathBuf,
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
            manifest.version == MANIFEST_VERSION,
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
        let resolved = config.resolve_topology()?;
        ensure!(
            manifest.firewall_mappings == super::build::firewall_mappings(&resolved),
            "firewall mappings do not match network.yaml"
        );
        for (index, (founder, artifact)) in resolved.iter().zip(&manifest.founders).enumerate() {
            let expected = format!("dist/validator-{index}.tgz");
            ensure!(
                artifact.index == index
                    && artifact.name == founder.name
                    && artifact.machine == founder.machine
                    && artifact.public_ip == founder.public_ip
                    && artifact.private_ip == founder.private_ip
                    && artifact.offset == founder.offset
                    && artifact.jump_user == founder.jump_user
                    && artifact.jump_port == founder.jump_port
                    && artifact.vm_user == founder.vm_user
                    && artifact.ports == founder.ports
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
            manifest,
        })
    }
}

#[derive(Clone)]
pub(super) struct SshOptions {
    ssh_key: Option<PathBuf>,
    known_hosts: PathBuf,
    identity: String,
}

impl SshOptions {
    pub(super) fn new(
        ssh_key: Option<&Path>,
        artifacts: &Path,
        manifest_identity: &str,
    ) -> Result<Self> {
        if let Some(path) = ssh_key {
            ensure!(path.is_file(), "SSH key does not exist: {}", path.display());
        }
        let state = artifacts
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(".outbe-up");
        fs::create_dir_all(&state)?;
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
        let identity = manifest_identity.chars().take(16).collect::<String>();
        let known_hosts = state.join(format!("known-hosts-{identity}"));
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&known_hosts)?;
        fs::set_permissions(&known_hosts, fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            ssh_key: ssh_key.map(Path::to_owned),
            known_hosts,
            identity,
        })
    }

    pub(super) fn capture(&self, founder: &FounderArtifact, command: &str) -> Result<String> {
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

    pub(super) fn run(&self, founder: &FounderArtifact, command: &str) -> Result<()> {
        let mut process = Command::new("ssh");
        self.ssh_args(&mut process, founder);
        let status = process.arg(command).status().wrap_err("run ssh")?;
        ensure!(status.success(), "SSH command failed on {}", founder.name);
        Ok(())
    }

    pub(super) fn run_quiet(&self, founder: &FounderArtifact, command: &str) -> Result<()> {
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
        founder: &FounderArtifact,
        sources: &[&Path],
        destination: &str,
        recursive: bool,
    ) -> Result<()> {
        let mut process = Command::new("scp");
        process.args(["-q", "-P", "22"]);
        self.transport_args(&mut process, founder);
        if let Some(ssh_key) = &self.ssh_key {
            process.arg("-i").arg(ssh_key);
        }
        if recursive {
            process.arg("-r");
        }
        process.args(sources);
        process.arg(format!(
            "{}@{}:{destination}",
            founder.vm_user,
            scp_host(&founder.private_ip.to_string())
        ));
        let status = process.status().wrap_err("run scp")?;
        ensure!(status.success(), "SCP failed for {}", founder.name);
        Ok(())
    }

    fn ssh_args(&self, process: &mut Command, founder: &FounderArtifact) {
        process.args(["-p", "22"]);
        self.transport_args(process, founder);
        if let Some(ssh_key) = &self.ssh_key {
            process.arg("-i").arg(ssh_key);
        }
        process.arg(format!("{}@{}", founder.vm_user, founder.private_ip));
    }

    fn transport_args(&self, process: &mut Command, founder: &FounderArtifact) {
        process.args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "StrictHostKeyChecking=accept-new",
            "-o",
            &format!("UserKnownHostsFile={}", self.known_hosts.display()),
            "-o",
            &format!("HostKeyAlias=outbe-{}-{}-vm", self.identity, founder.name),
            "-o",
            &format!("ProxyCommand={}", self.proxy_command(founder)),
        ]);
    }

    fn proxy_command(&self, founder: &FounderArtifact) -> String {
        let mut args = vec![
            "ssh".to_owned(),
            "-W".to_owned(),
            "%h:%p".to_owned(),
            "-p".to_owned(),
            founder.jump_port.to_string(),
            "-o".to_owned(),
            "BatchMode=yes".to_owned(),
            "-o".to_owned(),
            "ConnectTimeout=10".to_owned(),
            "-o".to_owned(),
            "IdentitiesOnly=yes".to_owned(),
            "-o".to_owned(),
            "StrictHostKeyChecking=accept-new".to_owned(),
            "-o".to_owned(),
            format!("UserKnownHostsFile={}", self.known_hosts.display()),
            "-o".to_owned(),
            format!(
                "HostKeyAlias=outbe-{}-{}-jump",
                self.identity, founder.machine
            ),
        ];
        if let Some(ssh_key) = &self.ssh_key {
            args.push("-i".to_owned());
            args.push(ssh_key.display().to_string());
        }
        args.push(format!("{}@{}", founder.jump_user, founder.public_ip));
        args.into_iter()
            .map(|argument| quote(&argument))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn scp_host(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::{build::FounderArtifact, config::ResolvedPorts};
    use std::net::Ipv4Addr;

    fn founder() -> FounderArtifact {
        FounderArtifact {
            index: 1,
            name: "validator-1".into(),
            machine: "fr5".into(),
            public_ip: Ipv4Addr::new(203, 0, 113, 10),
            private_ip: Ipv4Addr::new(10, 77, 0, 12),
            offset: 1,
            jump_user: "ubuntu".into(),
            jump_port: 22,
            vm_user: "root".into(),
            ports: ResolvedPorts {
                reth_p2p: 30_304,
                reth_discovery: 31_304,
                consensus: 30_401,
                radicle: 8_777,
                hyperlane_public: 9_001,
                rpc_public: None,
            },
            validator_address: String::new(),
            hyperlane_address: String::new(),
            consensus_public_key: String::new(),
            archive: "dist/validator-1.tgz".into(),
            archive_sha256: String::new(),
        }
    }

    #[test]
    fn ssh_and_scp_share_the_explicit_jump_transport() {
        let options = SshOptions {
            ssh_key: Some(PathBuf::from("/keys/operator")),
            known_hosts: PathBuf::from("/state/known-hosts"),
            identity: "0123456789abcdef".into(),
        };
        let founder = founder();
        let proxy = options.proxy_command(&founder);
        assert!(proxy.contains("ssh -W %h:%p"));
        assert!(proxy.contains("-i /keys/operator"));
        assert!(proxy.contains("ubuntu@203.0.113.10"));
        assert!(proxy.contains("HostKeyAlias=outbe-0123456789abcdef-fr5-jump"));

        let mut command = Command::new("ssh");
        options.ssh_args(&mut command, &founder);
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args.iter().any(|arg| arg.starts_with("ProxyCommand=ssh ")));
        assert!(args.contains(&"HostKeyAlias=outbe-0123456789abcdef-validator-1-vm".into()));
        assert_eq!(args.last().map(String::as_str), Some("root@10.77.0.12"));
    }
}
