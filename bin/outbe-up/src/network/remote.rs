use std::{
    path::{Path, PathBuf},
    process::Command,
};

use eyre::{bail, ensure, Context, Result};
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

#[derive(Clone)]
pub(crate) struct SshOptions {
    ssh_key: Option<PathBuf>,
    ssh_config: Option<PathBuf>,
}

impl SshOptions {
    pub(crate) fn new(ssh_key: Option<&Path>, ssh_config: Option<&Path>) -> Result<Self> {
        if let Some(path) = ssh_key {
            ensure!(path.is_file(), "SSH key does not exist: {}", path.display());
        }
        if let Some(path) = ssh_config {
            // Allow `/dev/null` (a character device, not a regular file).
            ensure!(
                path.exists(),
                "SSH config does not exist: {}",
                path.display()
            );
        }
        Ok(Self {
            ssh_key: ssh_key.map(Path::to_owned),
            ssh_config: ssh_config.map(Path::to_owned),
        })
    }

    pub(super) fn capture(&self, founder: &Founder, command: &str) -> Result<String> {
        self.capture_ssh(founder_ssh(founder)?, &founder.name, command)
    }

    pub(crate) fn capture_ssh(&self, ssh: &Ssh, label: &str, command: &str) -> Result<String> {
        Ok(self.capture_ssh_raw(ssh, label, command)?.trim().to_owned())
    }

    /// Capture stdout from SSH without trimming trailing whitespace or newlines.
    pub(crate) fn capture_ssh_raw(&self, ssh: &Ssh, label: &str, command: &str) -> Result<String> {
        let mut process = Command::new("ssh");
        self.ssh_endpoint_args(&mut process, ssh);
        let output = process.arg(command).output().wrap_err("run ssh")?;
        if !output.status.success() {
            bail!(
                "SSH command failed on {label}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub(crate) fn capture_ssh_stdin(
        &self,
        ssh: &Ssh,
        label: &str,
        command: &str,
        stdin_bytes: &[u8],
    ) -> Result<String> {
        use std::{io::Write, process::Stdio};

        let mut process = Command::new("ssh");
        self.ssh_endpoint_args(&mut process, ssh);
        let mut child = process
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .wrap_err("spawn ssh")?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(stdin_bytes).wrap_err("write ssh stdin")?;
        }
        let output = child.wait_with_output().wrap_err("wait for ssh")?;
        if !output.status.success() {
            bail!(
                "SSH command failed on {label}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub(super) fn run(&self, founder: &Founder, command: &str) -> Result<()> {
        let mut process = Command::new("ssh");
        self.ssh_args(&mut process, founder)?;
        let status = process.arg(command).status().wrap_err("run ssh")?;
        ensure!(status.success(), "SSH command failed on {}", founder.name);
        Ok(())
    }

    pub(super) fn run_quiet(&self, founder: &Founder, command: &str) -> Result<()> {
        self.run_quiet_ssh(founder_ssh(founder)?, &founder.name, command)
    }

    pub(crate) fn run_quiet_ssh(&self, ssh: &Ssh, label: &str, command: &str) -> Result<()> {
        let mut process = Command::new("ssh");
        self.ssh_endpoint_args(&mut process, ssh);
        let output = process.arg(command).output().wrap_err("run ssh")?;
        if !output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let details = [stdout.trim(), stderr.trim()]
                .into_iter()
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            bail!("SSH command failed on {label}:\n{details}");
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
        self.copy_to_ssh(
            founder_ssh(founder)?,
            &founder.name,
            sources,
            destination,
            recursive,
        )
    }

    /// Spawn an SSH command and stream arbitrary bytes into its stdin via `writer`.
    pub(crate) fn stream_stdin_to_ssh<F>(
        &self,
        ssh: &Ssh,
        label: &str,
        command: &str,
        writer: F,
    ) -> Result<String>
    where
        F: FnOnce(&mut dyn std::io::Write) -> Result<()>,
    {
        use std::process::Stdio;

        let mut process = Command::new("ssh");
        self.ssh_endpoint_args(&mut process, ssh);
        let mut child = process
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .wrap_err("spawn ssh")?;
        let write_result = if let Some(mut stdin) = child.stdin.take() {
            writer(&mut stdin)
        } else {
            Ok(())
        };
        let output = child.wait_with_output().wrap_err("wait for ssh")?;
        if let Err(err) = write_result {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.trim().is_empty() {
                return Err(err);
            }
            bail!(
                "streaming to SSH on {label} failed ({err:#}): {}",
                stderr.trim()
            );
        }
        if !output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let details = [stdout.trim(), stderr.trim()]
                .into_iter()
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            bail!("SSH command failed on {label}:\n{details}");
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub(crate) fn copy_to_ssh(
        &self,
        ssh: &Ssh,
        label: &str,
        sources: &[&Path],
        destination: &str,
        recursive: bool,
    ) -> Result<()> {
        let mut process = Command::new("scp");
        self.scp_endpoint_args(&mut process, ssh);
        if recursive {
            process.arg("-r");
        }
        process.args(sources);
        process.arg(format!(
            "{}@{}:{destination}",
            ssh.user,
            scp_host(&ssh.host)
        ));
        let status = process.status().wrap_err("run scp")?;
        ensure!(status.success(), "SCP failed for {label}");
        Ok(())
    }

    fn ssh_args(&self, process: &mut Command, founder: &Founder) -> Result<()> {
        self.ssh_endpoint_args(process, founder_ssh(founder)?);
        Ok(())
    }

    fn ssh_endpoint_args(&self, process: &mut Command, ssh: &Ssh) {
        if let Some(ssh_config) = &self.ssh_config {
            process.arg("-F").arg(ssh_config);
        }
        process.args(["-p", &ssh.port.to_string()]);
        process.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]);
        if let Some(ssh_key) = &self.ssh_key {
            process.arg("-i").arg(ssh_key);
        }
        process.arg(format!("{}@{}", ssh.user, ssh.host));
    }

    fn scp_endpoint_args(&self, process: &mut Command, ssh: &Ssh) {
        process.arg("-q");
        if let Some(ssh_config) = &self.ssh_config {
            process.arg("-F").arg(ssh_config);
        }
        process.args(["-P", &ssh.port.to_string()]);
        process.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]);
        if let Some(ssh_key) = &self.ssh_key {
            process.arg("-i").arg(ssh_key);
        }
    }
}

/// Single-quote a string for POSIX shells.
pub(crate) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn founder_ssh(founder: &Founder) -> Result<&Ssh> {
    founder
        .ssh
        .as_ref()
        .ok_or_else(|| eyre::eyre!("founder {} has no host SSH endpoint", founder.name))
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

    #[test]
    fn ssh_options_passes_dash_f_only_when_configured() {
        let endpoint = Ssh {
            host: "192.0.2.50".into(),
            user: "ubuntu".into(),
            port: 2222,
        };

        // Default leaves `-F` unset.
        let default_opts = SshOptions::new(None, None).unwrap();
        let mut ssh_cmd = Command::new("ssh");
        default_opts.ssh_endpoint_args(&mut ssh_cmd, &endpoint);
        let ssh_args: Vec<_> = ssh_cmd.get_args().collect();
        assert!(!ssh_args.contains(&std::ffi::OsStr::new("-F")));

        let mut scp_cmd = Command::new("scp");
        default_opts.scp_endpoint_args(&mut scp_cmd, &endpoint);
        let scp_args: Vec<_> = scp_cmd.get_args().collect();
        assert!(!scp_args.contains(&std::ffi::OsStr::new("-F")));

        // Explicit `--ssh-config /dev/null` is accepted and passes `-F /dev/null`.
        let null_opts = SshOptions::new(None, Some(Path::new("/dev/null"))).unwrap();
        let mut ssh_cmd = Command::new("ssh");
        null_opts.ssh_endpoint_args(&mut ssh_cmd, &endpoint);
        let ssh_args: Vec<_> = ssh_cmd.get_args().collect();
        assert!(ssh_args.windows(2).any(|w| {
            w[0] == std::ffi::OsStr::new("-F") && w[1] == std::ffi::OsStr::new("/dev/null")
        }));

        let mut scp_cmd = Command::new("scp");
        null_opts.scp_endpoint_args(&mut scp_cmd, &endpoint);
        let scp_args: Vec<_> = scp_cmd.get_args().collect();
        assert!(scp_args.windows(2).any(|w| {
            w[0] == std::ffi::OsStr::new("-F") && w[1] == std::ffi::OsStr::new("/dev/null")
        }));

        // Missing config path fails closed.
        assert!(SshOptions::new(None, Some(Path::new("/nonexistent/outbe-ssh-config"))).is_err());
    }
}
