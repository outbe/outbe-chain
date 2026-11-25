//! SSH/SCP command execution and SSH-tunneled JSON-RPC transport.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use eyre::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Ssh {
    pub host: String,
    #[serde(default = "default_ssh_user")]
    pub user: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
}

pub fn validate_ssh(ssh: &Ssh, label: &str) -> Result<()> {
    ensure!(safe_host(&ssh.host), "invalid SSH host for {label}");
    ensure!(safe_user(&ssh.user), "invalid SSH user for {label}");
    ensure!(ssh.port > 0, "SSH port must be non-zero for {label}");
    Ok(())
}

#[derive(Clone)]
pub struct SshOptions {
    ssh_key: Option<PathBuf>,
    ssh_config: Option<PathBuf>,
}

impl SshOptions {
    pub fn new(ssh_key: Option<&Path>, ssh_config: Option<&Path>) -> Result<Self> {
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

    pub fn capture_ssh(&self, ssh: &Ssh, label: &str, command: &str) -> Result<String> {
        Ok(self.capture_ssh_raw(ssh, label, command)?.trim().to_owned())
    }

    /// Capture stdout from SSH without trimming trailing whitespace or newlines.
    pub fn capture_ssh_raw(&self, ssh: &Ssh, label: &str, command: &str) -> Result<String> {
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

    pub fn capture_ssh_stdin(
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

    pub fn run_ssh(&self, ssh: &Ssh, label: &str, command: &str) -> Result<()> {
        let mut process = Command::new("ssh");
        self.ssh_endpoint_args(&mut process, ssh);
        let status = process.arg(command).status().wrap_err("run ssh")?;
        ensure!(status.success(), "SSH command failed on {label}");
        Ok(())
    }

    pub fn run_quiet_ssh(&self, ssh: &Ssh, label: &str, command: &str) -> Result<()> {
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

    /// Spawn an SSH command and stream arbitrary bytes into its stdin via `writer`.
    pub fn stream_stdin_to_ssh<F>(
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

    pub fn copy_to_ssh(
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

/// Execute `curl` on a remote host over SSH when a JSON-RPC endpoint is only reachable from that host.
#[derive(Clone)]
pub struct SshJsonRpc {
    url: String,
    ssh: Ssh,
    ssh_options: SshOptions,
    label: String,
}

impl SshJsonRpc {
    pub fn new(
        url: impl Into<String>,
        ssh: Ssh,
        ssh_options: SshOptions,
        label: impl Into<String>,
    ) -> Self {
        Self {
            url: url.into(),
            ssh,
            ssh_options,
            label: label.into(),
        }
    }

    pub async fn post_json(&self, payload: Vec<u8>) -> Result<String> {
        let command = format!(
            "curl -fsS --connect-timeout 5 --max-time 30 -H 'content-type: application/json' --data-binary @- {}",
            shell_quote(&self.url)
        );
        let ssh = self.ssh.clone();
        let ssh_options = self.ssh_options.clone();
        let label = self.label.clone();
        tokio::task::spawn_blocking(move || {
            ssh_options.capture_ssh_stdin(&ssh, &label, &command, &payload)
        })
        .await
        .wrap_err("SSH RPC task panicked")?
    }
}

/// Single-quote a string for POSIX shells.
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn scp_host(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    }
}

fn safe_host(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'-' | b'_'))
}

fn safe_user(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn default_ssh_user() -> String {
    "ubuntu".to_owned()
}

const fn default_ssh_port() -> u16 {
    22
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
