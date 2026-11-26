//! SSH/SCP command execution and SSH-tunneled JSON-RPC transport.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};

use eyre::{bail, ensure, Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

mod command;

use command::{
    capture_raw_with, capture_stdin_with, ensure_quiet_success, run_quiet_with, run_scp, run_with,
};

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

#[derive(Clone, Debug)]
struct KnownHostsPin {
    path: PathBuf,
    identity: String,
}

#[derive(Clone)]
pub struct SshOptions {
    ssh_key: Option<PathBuf>,
    ssh_config: Option<PathBuf>,
    known_hosts: Option<KnownHostsPin>,
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
            known_hosts: None,
        })
    }

    /// Pin SSH host keys per network inside `$XDG_STATE_HOME/outbe-net-up/known-hosts-<id16>`
    /// (`0700` dir, `0600` file) with `StrictHostKeyChecking=accept-new` and per-network
    /// `HostKeyAlias`, keeping operator SSH trust isolated from `~/.ssh/known_hosts`.
    pub fn with_network_known_hosts(mut self, manifest_identity: &str) -> Result<Self> {
        let state_dir = operator_state_dir("outbe-net-up")?;
        let pin = prepare_known_hosts_in(&state_dir, manifest_identity)?;
        self.known_hosts = Some(pin);
        Ok(self)
    }

    /// Pin SSH host keys to an explicit `known_hosts` file path for one network identity.
    pub fn with_known_hosts_file(mut self, path: PathBuf, manifest_identity: &str) -> Result<Self> {
        let identity = sanitize_network_identity(manifest_identity)?;
        self.known_hosts = Some(KnownHostsPin { path, identity });
        Ok(self)
    }

    pub fn capture_ssh(&self, ssh: &Ssh, label: &str, command: &str) -> Result<String> {
        Ok(self.capture_ssh_raw(ssh, label, command)?.trim().to_owned())
    }

    /// Capture stdout from SSH without trimming trailing whitespace or newlines.
    pub fn capture_ssh_raw(&self, ssh: &Ssh, label: &str, command: &str) -> Result<String> {
        capture_raw_with(self.ssh_command(ssh, None), label, command)
    }

    pub fn capture_ssh_stdin(
        &self,
        ssh: &Ssh,
        label: &str,
        command: &str,
        stdin_bytes: &[u8],
    ) -> Result<String> {
        capture_stdin_with(self.ssh_command(ssh, None), label, command, stdin_bytes)
    }

    pub fn run_ssh(&self, ssh: &Ssh, label: &str, command: &str) -> Result<()> {
        run_with(self.ssh_command(ssh, None), label, command)
    }

    pub fn run_quiet_ssh(&self, ssh: &Ssh, label: &str, command: &str) -> Result<()> {
        run_quiet_with(self.ssh_command(ssh, None), label, command)
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
        let mut child = self
            .ssh_command(ssh, None)
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
        ensure_quiet_success(&output, label)?;
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
        let process = self.scp_copy_command(ssh, None, sources, destination, recursive);
        run_scp(process, label)
    }

    /// Build an `ssh` command for `ssh`, optionally multiplexed over a ControlMaster socket.
    fn ssh_command(&self, ssh: &Ssh, control_path: Option<&Path>) -> Command {
        let mut process = Command::new("ssh");
        self.ssh_endpoint_args_with_control(&mut process, ssh, control_path);
        process
    }

    /// Build an `scp` command that copies `sources` to `destination` on `ssh`.
    fn scp_copy_command(
        &self,
        ssh: &Ssh,
        control_path: Option<&Path>,
        sources: &[&Path],
        destination: &str,
        recursive: bool,
    ) -> Command {
        let mut process = Command::new("scp");
        self.scp_endpoint_args_with_control(&mut process, ssh, control_path);
        if recursive {
            process.arg("-r");
        }
        process.args(sources);
        process.arg(format!(
            "{}@{}:{destination}",
            ssh.user,
            scp_host(&ssh.host)
        ));
        process
    }

    fn ssh_endpoint_args_with_control(
        &self,
        process: &mut Command,
        ssh: &Ssh,
        control_path: Option<&Path>,
    ) {
        self.config_args(process);
        process.args(["-p", &ssh.port.to_string()]);
        self.connection_args(process, ssh, control_path);
        process.arg(format!("{}@{}", ssh.user, ssh.host));
    }

    fn scp_endpoint_args_with_control(
        &self,
        process: &mut Command,
        ssh: &Ssh,
        control_path: Option<&Path>,
    ) {
        process.arg("-q");
        self.config_args(process);
        process.args(["-P", &ssh.port.to_string()]);
        self.connection_args(process, ssh, control_path);
    }

    /// Pass the operator SSH config file with `-F` when one is configured.
    fn config_args(&self, process: &mut Command) {
        if let Some(ssh_config) = &self.ssh_config {
            process.arg("-F").arg(ssh_config);
        }
    }

    /// Add the batch, host key, ControlMaster, and identity options that `ssh` and `scp` share.
    fn connection_args(&self, process: &mut Command, ssh: &Ssh, control_path: Option<&Path>) {
        process.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]);
        self.known_hosts_args(process, ssh);
        if let Some(ctrl) = control_path {
            process.args(["-o", "ControlMaster=auto", "-o", "ControlPersist=60s"]);
            process
                .arg("-o")
                .arg(format!("ControlPath={}", ctrl.display()));
        }
        if let Some(ssh_key) = &self.ssh_key {
            if self.known_hosts.is_some() {
                process.args(["-o", "IdentitiesOnly=yes"]);
            }
            process.arg("-i").arg(ssh_key);
        }
    }

    fn known_hosts_args(&self, process: &mut Command, ssh: &Ssh) {
        if let Some(pin) = &self.known_hosts {
            process.args([
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-o",
                &format!("UserKnownHostsFile={}", pin.path.display()),
                "-o",
                &format!(
                    "HostKeyAlias=outbe-{}-{}-{}",
                    pin.identity,
                    sanitize_host_alias(&ssh.host),
                    ssh.port
                ),
            ]);
        }
    }
}

/// Multiplexed SSH/SCP executor using per-run `0700` ControlMaster sockets.
///
/// Opens at most one persistent SSH connection per unique `(user, host, port)` and cleanly shuts
/// down every master socket with `ssh -O exit` when dropped.
pub struct SshControlSession {
    options: SshOptions,
    control_dir: tempfile::TempDir,
    sockets: Mutex<BTreeMap<String, (Ssh, PathBuf)>>,
}

impl SshControlSession {
    pub fn new(ssh_key: Option<&Path>, ssh_config: Option<&Path>) -> Result<Self> {
        let options = SshOptions::new(ssh_key, ssh_config)?;
        Self::from_options(options)
    }

    /// Create a ControlMaster session pinned to per-network `known_hosts` under
    /// `$XDG_STATE_HOME/outbe-net-up/known-hosts-<id16>` (`0700`/`0600`).
    pub fn new_for_network(
        ssh_key: Option<&Path>,
        ssh_config: Option<&Path>,
        manifest_identity: &str,
    ) -> Result<Self> {
        let options =
            SshOptions::new(ssh_key, ssh_config)?.with_network_known_hosts(manifest_identity)?;
        Self::from_options(options)
    }

    fn from_options(options: SshOptions) -> Result<Self> {
        let control_dir = tempfile::Builder::new()
            .prefix("outbe-ssh-")
            .tempdir()
            .wrap_err("create private SSH ControlMaster socket directory")?;
        fs::set_permissions(control_dir.path(), fs::Permissions::from_mode(0o700))
            .wrap_err("set 0700 mode on SSH ControlMaster directory")?;
        Ok(Self {
            options,
            control_dir,
            sockets: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn control_dir(&self) -> &Path {
        self.control_dir.path()
    }

    fn socket_for(&self, ssh: &Ssh) -> PathBuf {
        let key = format!("{}@{}:{}", ssh.user, ssh.host, ssh.port);
        let mut guard = self.sockets.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .entry(key.clone())
            .or_insert_with(|| {
                let hash = fnv1a_64(key.as_bytes());
                let sock = self.control_dir.path().join(format!("{hash:016x}.sock"));
                (ssh.clone(), sock)
            })
            .1
            .clone()
    }

    /// Gracefully terminate all active SSH ControlMaster connections (`ssh -O exit`).
    pub fn close_all(&self) {
        let entries: Vec<(Ssh, PathBuf)> = {
            let mut guard = self.sockets.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *guard).into_values().collect()
        };
        for (ssh, sock) in entries {
            if !sock.exists() {
                continue;
            }
            let mut cmd = Command::new("ssh");
            self.options.config_args(&mut cmd);
            cmd.args(["-p", &ssh.port.to_string()]);
            cmd.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=5"]);
            cmd.arg("-S").arg(&sock);
            cmd.args(["-O", "exit"]);
            cmd.arg(format!("{}@{}", ssh.user, ssh.host));
            cmd.stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let _ = cmd.status();
        }
    }

    /// Build an `ssh` command multiplexed over the ControlMaster socket for `ssh`.
    fn ssh_command(&self, ssh: &Ssh) -> Command {
        let sock = self.socket_for(ssh);
        self.options.ssh_command(ssh, Some(&sock))
    }
}

impl Drop for SshControlSession {
    fn drop(&mut self) {
        self.close_all();
    }
}

impl HostExec for SshControlSession {
    fn capture(&self, ssh: &Ssh, label: &str, command: &str) -> Result<String> {
        Ok(self.capture_raw(ssh, label, command)?.trim().to_owned())
    }

    fn capture_raw(&self, ssh: &Ssh, label: &str, command: &str) -> Result<String> {
        capture_raw_with(self.ssh_command(ssh), label, command)
    }

    fn capture_stdin(
        &self,
        ssh: &Ssh,
        label: &str,
        command: &str,
        stdin_bytes: &[u8],
    ) -> Result<String> {
        capture_stdin_with(self.ssh_command(ssh), label, command, stdin_bytes)
    }

    fn run(&self, ssh: &Ssh, label: &str, command: &str) -> Result<()> {
        run_with(self.ssh_command(ssh), label, command)
    }

    fn run_quiet(&self, ssh: &Ssh, label: &str, command: &str) -> Result<()> {
        run_quiet_with(self.ssh_command(ssh), label, command)
    }

    fn copy_to(
        &self,
        ssh: &Ssh,
        label: &str,
        sources: &[&Path],
        destination: &str,
        recursive: bool,
    ) -> Result<()> {
        let sock = self.socket_for(ssh);
        let process =
            self.options
                .scp_copy_command(ssh, Some(&sock), sources, destination, recursive);
        run_scp(process, label)
    }
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Object-safe remote host execution interface implemented by [`SshOptions`] and fake test
/// executors so multi-host orchestration (`outbe-net-up`) can be tested deterministically without
/// real SSH servers.
pub trait HostExec: Send + Sync {
    /// Run a remote shell command and return trimmed stdout.
    fn capture(&self, ssh: &Ssh, label: &str, command: &str) -> Result<String>;

    /// Run a remote shell command and return exact raw stdout (no trimming).
    fn capture_raw(&self, ssh: &Ssh, label: &str, command: &str) -> Result<String>;

    /// Run a remote shell command with `stdin_bytes` piped to stdin and return raw stdout.
    fn capture_stdin(
        &self,
        ssh: &Ssh,
        label: &str,
        command: &str,
        stdin_bytes: &[u8],
    ) -> Result<String>;

    /// Run a remote shell command inheriting stdio.
    fn run(&self, ssh: &Ssh, label: &str, command: &str) -> Result<()>;

    /// Run a remote shell command quietly, surfacing stdout+stderr only on failure.
    fn run_quiet(&self, ssh: &Ssh, label: &str, command: &str) -> Result<()>;

    /// Copy one or more local paths to `destination` on the remote host.
    fn copy_to(
        &self,
        ssh: &Ssh,
        label: &str,
        sources: &[&Path],
        destination: &str,
        recursive: bool,
    ) -> Result<()>;
}

impl HostExec for SshOptions {
    fn capture(&self, ssh: &Ssh, label: &str, command: &str) -> Result<String> {
        self.capture_ssh(ssh, label, command)
    }

    fn capture_raw(&self, ssh: &Ssh, label: &str, command: &str) -> Result<String> {
        self.capture_ssh_raw(ssh, label, command)
    }

    fn capture_stdin(
        &self,
        ssh: &Ssh,
        label: &str,
        command: &str,
        stdin_bytes: &[u8],
    ) -> Result<String> {
        self.capture_ssh_stdin(ssh, label, command, stdin_bytes)
    }

    fn run(&self, ssh: &Ssh, label: &str, command: &str) -> Result<()> {
        self.run_ssh(ssh, label, command)
    }

    fn run_quiet(&self, ssh: &Ssh, label: &str, command: &str) -> Result<()> {
        self.run_quiet_ssh(ssh, label, command)
    }

    fn copy_to(
        &self,
        ssh: &Ssh,
        label: &str,
        sources: &[&Path],
        destination: &str,
        recursive: bool,
    ) -> Result<()> {
        self.copy_to_ssh(ssh, label, sources, destination, recursive)
    }
}

impl<T: HostExec + ?Sized> HostExec for Arc<T> {
    fn capture(&self, ssh: &Ssh, label: &str, command: &str) -> Result<String> {
        (**self).capture(ssh, label, command)
    }

    fn capture_raw(&self, ssh: &Ssh, label: &str, command: &str) -> Result<String> {
        (**self).capture_raw(ssh, label, command)
    }

    fn capture_stdin(
        &self,
        ssh: &Ssh,
        label: &str,
        command: &str,
        stdin_bytes: &[u8],
    ) -> Result<String> {
        (**self).capture_stdin(ssh, label, command, stdin_bytes)
    }

    fn run(&self, ssh: &Ssh, label: &str, command: &str) -> Result<()> {
        (**self).run(ssh, label, command)
    }

    fn run_quiet(&self, ssh: &Ssh, label: &str, command: &str) -> Result<()> {
        (**self).run_quiet(ssh, label, command)
    }

    fn copy_to(
        &self,
        ssh: &Ssh,
        label: &str,
        sources: &[&Path],
        destination: &str,
        recursive: bool,
    ) -> Result<()> {
        (**self).copy_to(ssh, label, sources, destination, recursive)
    }
}

/// Run a remote command expected to emit JSON on stdout (such as `outbe-up ... --json`) and
/// deserialize the result.
pub fn run_host_json<E: HostExec + ?Sized, T: DeserializeOwned>(
    exec: &E,
    ssh: &Ssh,
    label: &str,
    command: &str,
) -> Result<T> {
    let raw = exec.capture_raw(ssh, label, command)?;
    serde_json::from_str(&raw).wrap_err_with(|| {
        format!(
            "parse JSON output from `{label}` (stdout snippet: {:?})",
            raw.chars().take(240).collect::<String>()
        )
    })
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

fn operator_state_dir(app_name: &str) -> Result<PathBuf> {
    let base =
        if let Some(path) = std::env::var_os("XDG_STATE_HOME").filter(|path| !path.is_empty()) {
            PathBuf::from(path)
        } else {
            let home = std::env::var_os("HOME")
                .filter(|path| !path.is_empty())
                .ok_or_else(|| {
                    eyre::eyre!("HOME is not set; cannot locate {app_name} operator state")
                })?;
            PathBuf::from(home).join(".local/state")
        };
    ensure!(
        base.is_absolute(),
        "{app_name} operator state directory must be absolute"
    );
    Ok(base.join(app_name))
}

fn sanitize_network_identity(manifest_identity: &str) -> Result<String> {
    let trimmed = manifest_identity
        .trim()
        .strip_prefix("0x")
        .unwrap_or(manifest_identity.trim());
    let id: String = trimmed.chars().take(16).collect();
    ensure!(
        !id.is_empty()
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')),
        "invalid network identity for SSH known_hosts pinning: {manifest_identity:?}"
    );
    Ok(id)
}

fn sanitize_host_alias(host: &str) -> String {
    host.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn prepare_known_hosts_in(state_dir: &Path, manifest_identity: &str) -> Result<KnownHostsPin> {
    let identity = sanitize_network_identity(manifest_identity)?;
    fs::create_dir_all(state_dir)?;
    fs::set_permissions(state_dir, fs::Permissions::from_mode(0o700))?;
    let path = state_dir.join(format!("known-hosts-{identity}"));
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    Ok(KnownHostsPin { path, identity })
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
mod tests;
