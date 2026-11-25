use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use eyre::{bail, ensure, Context, Result};
use outbe_genesis_cli::launch_bundle::quote;
use outbe_software_bundle::sha256_file;

use super::{
    build::{BuildState, FounderArtifact, JoiningArtifact, NetworkManifest, MANIFEST_VERSION},
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
        verify_joining_artifacts(root, &manifest.joining)?;
        Ok(Self {
            root: root.to_owned(),
            manifest,
        })
    }
}

fn verify_joining_artifacts(root: &Path, joining: &JoiningArtifact) -> Result<()> {
    ensure!(
        joining.bundle == "joining/outbe-validator-operator-bundle.tar.zst"
            && joining.config_example == "joining/operator-config.example.yaml",
        "joining artifacts do not use the expected immutable paths"
    );
    ensure!(
        sha256_file(&root.join(&joining.bundle))? == joining.bundle_sha256,
        "joining operator bundle changed after network build"
    );
    ensure!(
        root.join(&joining.config_example).is_file(),
        "joining operator config example is missing"
    );
    Ok(())
}

#[derive(Clone)]
pub(super) struct SshOptions {
    ssh_key: Option<PathBuf>,
    known_hosts: PathBuf,
    identity: String,
}

impl SshOptions {
    pub(super) fn new(ssh_key: Option<&Path>, manifest_identity: &str) -> Result<Self> {
        if let Some(path) = ssh_key {
            ensure!(path.is_file(), "SSH key does not exist: {}", path.display());
        }
        let state = operator_state_dir()?;
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

    pub(super) fn copy_root_file_from(
        &self,
        founder: &FounderArtifact,
        source: &str,
        destination: &Path,
    ) -> Result<()> {
        ensure!(
            !destination.exists(),
            "download destination already exists: {}",
            destination.display()
        );
        let parent = destination
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let temporary = tempfile::NamedTempFile::new_in(parent)?;
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o600))?;
        let output = temporary.reopen()?;
        let source = quote(source);
        let command =
            format!("if [ \"$(id -u)\" -eq 0 ]; then cat {source}; else sudo -n cat {source}; fi");
        let mut process = Command::new("ssh");
        self.ssh_args(&mut process, founder);
        let status = process
            .arg(command)
            .stdout(Stdio::from(output))
            .status()
            .wrap_err("download root-owned file over ssh")?;
        ensure!(status.success(), "SSH download failed for {}", founder.name);
        temporary.as_file().sync_all()?;
        temporary
            .persist_noclobber(destination)
            .map_err(|error| error.error)
            .wrap_err_with(|| format!("persist download: {}", destination.display()))?;
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
            "StrictHostKeyChecking=accept-new",
            "-o",
            &format!("UserKnownHostsFile={}", self.known_hosts.display()),
            "-o",
            &format!("HostKeyAlias=outbe-{}-{}-vm", self.identity, founder.name),
            "-o",
            &format!("ProxyCommand={}", self.proxy_command(founder)),
        ]);
        if self.ssh_key.is_some() {
            process.args(["-o", "IdentitiesOnly=yes"]);
        }
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
            args.push("-o".to_owned());
            args.push("IdentitiesOnly=yes".to_owned());
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

fn operator_state_dir() -> Result<PathBuf> {
    let base = if let Some(path) =
        std::env::var_os("XDG_STATE_HOME").filter(|path| !path.is_empty())
    {
        PathBuf::from(path)
    } else {
        let home = std::env::var_os("HOME")
            .filter(|path| !path.is_empty())
            .ok_or_else(|| eyre::eyre!("HOME is not set; cannot locate outbe-up operator state"))?;
        PathBuf::from(home).join(".local/state")
    };
    ensure!(
        base.is_absolute(),
        "outbe-up operator state directory must be absolute"
    );
    Ok(base.join("outbe-up"))
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
        assert!(proxy.contains("IdentitiesOnly=yes"));
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

        let agent_options = SshOptions {
            ssh_key: None,
            known_hosts: PathBuf::from("/state/known-hosts"),
            identity: "0123456789abcdef".into(),
        };
        assert!(!agent_options
            .proxy_command(&founder)
            .contains("IdentitiesOnly"));
        let mut agent_command = Command::new("ssh");
        agent_options.ssh_args(&mut agent_command, &founder);
        assert!(!agent_command
            .get_args()
            .any(|argument| argument == "IdentitiesOnly=yes"));
    }

    #[test]
    fn joining_bundle_is_bound_to_the_network_manifest() {
        let root = tempfile::tempdir().unwrap();
        let joining = root.path().join("joining");
        fs::create_dir(&joining).unwrap();
        let bundle = joining.join("outbe-validator-operator-bundle.tar.zst");
        fs::write(&bundle, b"operator bundle").unwrap();
        fs::write(
            joining.join("operator-config.example.yaml"),
            b"version: 1\n",
        )
        .unwrap();
        let artifact = JoiningArtifact {
            bundle: "joining/outbe-validator-operator-bundle.tar.zst".into(),
            bundle_sha256: sha256_file(&bundle).unwrap(),
            config_example: "joining/operator-config.example.yaml".into(),
        };
        verify_joining_artifacts(root.path(), &artifact).unwrap();

        fs::write(&bundle, b"changed bundle").unwrap();
        assert!(verify_joining_artifacts(root.path(), &artifact).is_err());
    }
}
