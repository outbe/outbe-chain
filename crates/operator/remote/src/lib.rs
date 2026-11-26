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
        let mut process = Command::new("ssh");
        self.ssh_endpoint_args(&mut process, ssh);
        let child = process
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .wrap_err("spawn ssh")?;
        run_child_with_stdin(child, label, stdin_bytes)
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
        self.ssh_endpoint_args_with_control(process, ssh, None);
    }

    fn scp_endpoint_args(&self, process: &mut Command, ssh: &Ssh) {
        self.scp_endpoint_args_with_control(process, ssh, None);
    }

    fn ssh_endpoint_args_with_control(
        &self,
        process: &mut Command,
        ssh: &Ssh,
        control_path: Option<&Path>,
    ) {
        if let Some(ssh_config) = &self.ssh_config {
            process.arg("-F").arg(ssh_config);
        }
        process.args(["-p", &ssh.port.to_string()]);
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
        process.arg(format!("{}@{}", ssh.user, ssh.host));
    }

    fn scp_endpoint_args_with_control(
        &self,
        process: &mut Command,
        ssh: &Ssh,
        control_path: Option<&Path>,
    ) {
        process.arg("-q");
        if let Some(ssh_config) = &self.ssh_config {
            process.arg("-F").arg(ssh_config);
        }
        process.args(["-P", &ssh.port.to_string()]);
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
            if let Some(ssh_config) = &self.options.ssh_config {
                cmd.arg("-F").arg(ssh_config);
            }
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

    fn capture_raw_inner(&self, ssh: &Ssh, label: &str, command: &str) -> Result<String> {
        let sock = self.socket_for(ssh);
        let mut process = Command::new("ssh");
        self.options
            .ssh_endpoint_args_with_control(&mut process, ssh, Some(&sock));
        let output = process.arg(command).output().wrap_err("run ssh")?;
        if !output.status.success() {
            bail!(
                "SSH command failed on {label}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn capture_stdin_inner(
        &self,
        ssh: &Ssh,
        label: &str,
        command: &str,
        stdin_bytes: &[u8],
    ) -> Result<String> {
        let sock = self.socket_for(ssh);
        let mut process = Command::new("ssh");
        self.options
            .ssh_endpoint_args_with_control(&mut process, ssh, Some(&sock));
        let child = process
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .wrap_err("spawn ssh")?;
        run_child_with_stdin(child, label, stdin_bytes)
    }
}

/// Stream `stdin_bytes` to `child` on a scoped writer thread while draining stdout/stderr via
/// `wait_with_output`. Tolerates `BrokenPipe` if the remote exits before consuming stdin so the
/// child is always reaped and remote stderr is surfaced as the primary error.
fn run_child_with_stdin(
    mut child: std::process::Child,
    label: &str,
    stdin_bytes: &[u8],
) -> Result<String> {
    use std::io::{ErrorKind, Write};

    let stdin = child.stdin.take();
    let (write_res, output) = std::thread::scope(|scope| {
        let writer = scope.spawn(move || -> std::io::Result<()> {
            if let Some(mut pipe) = stdin {
                if let Err(err) = pipe.write_all(stdin_bytes) {
                    if err.kind() != ErrorKind::BrokenPipe {
                        return Err(err);
                    }
                }
            }
            Ok(())
        });
        let output = child.wait_with_output().wrap_err("wait for ssh");
        let write_res = match writer.join() {
            Ok(res) => res,
            Err(_) => Err(std::io::Error::other("ssh stdin writer thread panicked")),
        };
        (write_res, output)
    });

    let output = output?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        if detail.is_empty() {
            if let Err(write_err) = write_res {
                bail!("SSH command failed on {label} (stdin write error: {write_err})");
            }
            let stdout = String::from_utf8_lossy(&output.stdout);
            bail!("SSH command failed on {label}: {}", stdout.trim());
        }
        bail!("SSH command failed on {label}: {detail}");
    }
    write_res.wrap_err("write ssh stdin")?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

impl Drop for SshControlSession {
    fn drop(&mut self) {
        self.close_all();
    }
}

impl HostExec for SshControlSession {
    fn capture(&self, ssh: &Ssh, label: &str, command: &str) -> Result<String> {
        Ok(self
            .capture_raw_inner(ssh, label, command)?
            .trim()
            .to_owned())
    }

    fn capture_raw(&self, ssh: &Ssh, label: &str, command: &str) -> Result<String> {
        self.capture_raw_inner(ssh, label, command)
    }

    fn capture_stdin(
        &self,
        ssh: &Ssh,
        label: &str,
        command: &str,
        stdin_bytes: &[u8],
    ) -> Result<String> {
        self.capture_stdin_inner(ssh, label, command, stdin_bytes)
    }

    fn run(&self, ssh: &Ssh, label: &str, command: &str) -> Result<()> {
        let sock = self.socket_for(ssh);
        let mut process = Command::new("ssh");
        self.options
            .ssh_endpoint_args_with_control(&mut process, ssh, Some(&sock));
        let status = process.arg(command).status().wrap_err("run ssh")?;
        ensure!(status.success(), "SSH command failed on {label}");
        Ok(())
    }

    fn run_quiet(&self, ssh: &Ssh, label: &str, command: &str) -> Result<()> {
        let sock = self.socket_for(ssh);
        let mut process = Command::new("ssh");
        self.options
            .ssh_endpoint_args_with_control(&mut process, ssh, Some(&sock));
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

    fn copy_to(
        &self,
        ssh: &Ssh,
        label: &str,
        sources: &[&Path],
        destination: &str,
        recursive: bool,
    ) -> Result<()> {
        let sock = self.socket_for(ssh);
        let mut process = Command::new("scp");
        self.options
            .scp_endpoint_args_with_control(&mut process, ssh, Some(&sock));
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

    #[test]
    fn host_exec_trait_supports_object_safe_fake_and_json_helper() {
        struct FakeExec;
        impl HostExec for FakeExec {
            fn capture(&self, _ssh: &Ssh, _label: &str, command: &str) -> Result<String> {
                Ok(command.trim().to_owned())
            }
            fn capture_raw(&self, _ssh: &Ssh, _label: &str, _command: &str) -> Result<String> {
                Ok(r#"{"name":"outbe-up","version":"0.1.0"}"#.to_owned())
            }
            fn capture_stdin(
                &self,
                _ssh: &Ssh,
                _label: &str,
                _command: &str,
                stdin_bytes: &[u8],
            ) -> Result<String> {
                Ok(String::from_utf8_lossy(stdin_bytes).into_owned())
            }
            fn run(&self, _ssh: &Ssh, _label: &str, _command: &str) -> Result<()> {
                Ok(())
            }
            fn run_quiet(&self, _ssh: &Ssh, _label: &str, _command: &str) -> Result<()> {
                Ok(())
            }
            fn copy_to(
                &self,
                _ssh: &Ssh,
                _label: &str,
                _sources: &[&Path],
                _destination: &str,
                _recursive: bool,
            ) -> Result<()> {
                Ok(())
            }
        }

        #[derive(Debug, Deserialize, Eq, PartialEq)]
        struct Resp {
            name: String,
            version: String,
        }

        let exec: Arc<dyn HostExec> = Arc::new(FakeExec);
        let ssh = Ssh {
            host: "192.0.2.50".into(),
            user: "ubuntu".into(),
            port: 22,
        };
        let parsed: Resp =
            run_host_json(exec.as_ref(), &ssh, "amd", "outbe-up version --json").unwrap();
        assert_eq!(
            parsed,
            Resp {
                name: "outbe-up".into(),
                version: "0.1.0".into(),
            }
        );
        assert_eq!(
            exec.capture_stdin(&ssh, "amd", "cat", b"hello").unwrap(),
            "hello"
        );
    }

    #[test]
    fn ssh_control_session_uses_0700_dir_short_socket_and_control_master_args() {
        let session = SshControlSession::new(None, Some(Path::new("/dev/null"))).unwrap();
        assert_eq!(
            fs::metadata(session.control_dir())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let endpoint = Ssh {
            host: "192.0.2.50".into(),
            user: "ubuntu".into(),
            port: 2222,
        };
        let sock = session.socket_for(&endpoint);
        assert!(sock.starts_with(session.control_dir()));
        assert!(
            sock.to_string_lossy().len() < 100,
            "socket path too long: {}",
            sock.display()
        );
        assert_eq!(sock, session.socket_for(&endpoint));

        let mut ssh_cmd = Command::new("ssh");
        session
            .options
            .ssh_endpoint_args_with_control(&mut ssh_cmd, &endpoint, Some(&sock));
        let ssh_args: Vec<String> = ssh_cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(ssh_args.contains(&"ControlMaster=auto".to_owned()));
        assert!(ssh_args.contains(&"ControlPersist=60s".to_owned()));
        assert!(ssh_args.contains(&format!("ControlPath={}", sock.display())));

        let mut scp_cmd = Command::new("scp");
        session
            .options
            .scp_endpoint_args_with_control(&mut scp_cmd, &endpoint, Some(&sock));
        let scp_args: Vec<String> = scp_cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(scp_args.contains(&"ControlMaster=auto".to_owned()));
        assert!(scp_args.contains(&format!("ControlPath={}", sock.display())));

        session.close_all();
    }

    #[test]
    fn run_child_with_stdin_surfaces_remote_stderr_on_early_exit_broken_pipe() {
        let child = Command::new("sh")
            .args(["-c", "echo 'outbe-up: staging lock busy' >&2; exit 1"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Large payload exceeds OS pipe buffer so writer hits BrokenPipe when child exits early.
        let large_stdin = vec![b'x'; 512 * 1024];
        let err = run_child_with_stdin(child, "host-a", &large_stdin).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("outbe-up: staging lock busy"),
            "expected remote stderr to be surfaced, got: {msg}"
        );
    }

    #[test]
    fn network_known_hosts_pins_accept_new_alias_and_permissions() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("outbe-net-up");
        let pin = prepare_known_hosts_in(
            &state_dir,
            "0123456789abcdef999999999999999999999999999999999999999999999999",
        )
        .unwrap();
        assert_eq!(pin.identity, "0123456789abcdef");
        assert_eq!(pin.path, state_dir.join("known-hosts-0123456789abcdef"));
        assert_eq!(
            fs::metadata(&state_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&pin.path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let key_file = tmp.path().join("id_ed25519");
        fs::write(&key_file, b"fake-key").unwrap();

        let endpoint = Ssh {
            host: "198.51.100.10".into(),
            user: "ubuntu".into(),
            port: 2222,
        };
        let opts = SshOptions::new(Some(&key_file), None)
            .unwrap()
            .with_known_hosts_file(pin.path.clone(), &pin.identity)
            .unwrap();

        let mut ssh_cmd = Command::new("ssh");
        opts.ssh_endpoint_args(&mut ssh_cmd, &endpoint);
        let ssh_args: Vec<String> = ssh_cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(ssh_args.contains(&"StrictHostKeyChecking=accept-new".to_owned()));
        assert!(ssh_args.contains(&format!("UserKnownHostsFile={}", pin.path.display())));
        assert!(
            ssh_args.contains(&"HostKeyAlias=outbe-0123456789abcdef-198.51.100.10-2222".to_owned())
        );
        assert!(ssh_args.contains(&"IdentitiesOnly=yes".to_owned()));

        let mut scp_cmd = Command::new("scp");
        opts.scp_endpoint_args(&mut scp_cmd, &endpoint);
        let scp_args: Vec<String> = scp_cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(scp_args.contains(&"StrictHostKeyChecking=accept-new".to_owned()));
        assert!(scp_args.contains(&format!("UserKnownHostsFile={}", pin.path.display())));
        assert!(
            scp_args.contains(&"HostKeyAlias=outbe-0123456789abcdef-198.51.100.10-2222".to_owned())
        );
        assert!(scp_args.contains(&"IdentitiesOnly=yes".to_owned()));

        // Without --ssh-key, IdentitiesOnly is not forced so agent/default identities still work.
        let agent_opts = SshOptions::new(None, None)
            .unwrap()
            .with_known_hosts_file(pin.path, &pin.identity)
            .unwrap();
        let mut agent_cmd = Command::new("ssh");
        agent_opts.ssh_endpoint_args(&mut agent_cmd, &endpoint);
        assert!(!agent_cmd.get_args().any(|arg| arg == "IdentitiesOnly=yes"));
    }
}
