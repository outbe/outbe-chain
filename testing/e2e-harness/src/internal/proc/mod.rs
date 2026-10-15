//! Owned processes and containers, plus the small IO helpers the node/enclave
//! launchers share.
//!
//! Every process the harness launches is **owned**. The harness holds nodes as
//! [`ChildGuard`]s (killed + reaped on drop, no `nohup`/pid-files) and enclave
//! containers as [`EnclaveGuard`]s. The `docker run` runs in the **foreground**
//! (no `-d`) as an owned child, with a `docker rm -f` backstop on drop. The
//! harness builds a fresh `World` per scenario, so dropping it removes everything.
//! The `Localnet`/`Nodes` handles that hold these guards are non-`Clone`.

use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::sleep;
use std::time::Duration;

use alloy_primitives::hex;
use eyre::{bail, eyre, Result, WrapErr};

mod enclave;
mod enclave_launch;
mod sgx_image;

pub(crate) use enclave::{spawn_enclave, spawn_enclave_ready, EnclaveGuard};
pub(crate) use enclave_launch::{EnclaveLaunch, EnclaveSpec, SealSpec, TestRemoteAttestation};
pub(crate) use sgx_image::{
    ensure_enclave_image, inspect_test_sgx_measurement, DockerImageId, MeasurementMounts,
};

const SENSITIVE_ARG_FLAGS: &[&str] = &["--private-key", "--p2p-secret-key-hex", "--dkg-seed"];

/// How long a node/enclave gets to exit on SIGTERM before it is killed. Reth
/// closes its database well inside this. The ceiling only bounds teardown when
/// a process is wedged.
pub(crate) const GRACEFUL_STOP_TIMEOUT: Duration = Duration::from_secs(15);

/// Preserve a command's diagnostic shape without emitting secret argument
/// values into CI logs, evidence capture, or agent transcripts.
pub(crate) fn redact_args_for_log(argv: &[String]) -> Vec<String> {
    let mut redacted = Vec::with_capacity(argv.len());
    let mut redact_next = false;
    for arg in argv {
        if redact_next {
            redacted.push("<redacted>".to_owned());
            redact_next = false;
            continue;
        }
        if SENSITIVE_ARG_FLAGS.iter().any(|flag| arg == flag) {
            redacted.push(arg.clone());
            redact_next = true;
            continue;
        }
        if let Some((flag, _)) = arg.split_once('=') {
            if SENSITIVE_ARG_FLAGS.contains(&flag) {
                redacted.push(format!("{flag}=<redacted>"));
                continue;
            }
        }
        redacted.push(arg.clone());
    }
    redacted
}

/// Build a `Vec<String>` of process arguments from `Display` tokens.
///
/// The macro converts every argument to a string once (`to_string`). Callers
/// write clean literals, ports, and paths without `.into()` / `.to_string()` /
/// `.display().to_string()` calls. For a path, pass its `.display()`. Use
/// `.extend(args![...])` to add conditional or role-specific tails to a base list.
macro_rules! args {
    ($($x:expr),* $(,)?) => {
        ::std::vec![$($x.to_string()),*]
    };
}
pub(crate) use args;

/// An owned child process: killed and reaped on drop.
#[derive(Debug)]
pub(crate) struct ChildGuard {
    label: String,
    child: Child,
    node_data_dir: Option<PathBuf>,
}

impl ChildGuard {
    pub(crate) fn spawn(label: impl Into<String>, mut cmd: Command) -> Result<Self> {
        let label = label.into();
        let argv = cmd.get_args().collect::<Vec<_>>();
        let node_data_dir = argv
            .windows(2)
            .find(|pair| pair[0] == "--datadir")
            .map(|pair| PathBuf::from(pair[1]))
            .or_else(|| {
                argv.iter().find_map(|arg| {
                    arg.to_str()
                        .and_then(|arg| arg.strip_prefix("--datadir="))
                        .map(PathBuf::from)
                })
            });
        let child = cmd.spawn().wrap_err_with(|| format!("spawn {label}"))?;
        Ok(Self {
            label,
            child,
            node_data_dir,
        })
    }

    pub(crate) fn owns_node_data_dir(&self, path: &Path) -> bool {
        self.node_data_dir.as_deref() == Some(path)
    }

    /// Whether the child has already exited (non-blocking).
    pub(crate) fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// Observe this exact owned child without discarding status or wait errors.
    pub(crate) fn exit_status(&mut self) -> Result<Option<ExitStatus>> {
        self.child
            .try_wait()
            .wrap_err_with(|| format!("observe owned child {}", self.label))
    }

    /// The OS process id (for `--debug` launch logging).
    pub(crate) fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Abrupt fault of this owned live child, without waiting for its peers.
    pub(crate) fn signal_fault(&mut self) -> Result<()> {
        if self.exit_status()?.is_some() {
            bail!(
                "owned child {} exited before the requested fault",
                self.label
            );
        }
        self.child
            .kill()
            .wrap_err_with(|| format!("SIGKILL owned child {}", self.label))
    }

    /// Reap a previously signalled child without sending another signal.
    pub(crate) fn wait_for_exit(&mut self, timeout: Duration) -> Result<ExitStatus> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(status) = self.exit_status()? {
                return Ok(status);
            }
            if std::time::Instant::now() >= deadline {
                bail!("owned child {} did not exit after signal", self.label);
            }
            sleep(Duration::from_millis(10));
        }
    }

    /// Observe a previously signalled fault without discarding wait errors.
    pub(crate) fn reap_fault(&mut self, timeout: Duration) -> Result<ExitStatus> {
        self.wait_for_exit(timeout)
    }

    /// Inject an abrupt fault into this live owned child and observe its exit.
    pub(crate) fn fault_and_reap(&mut self) -> Result<ExitStatus> {
        self.signal_fault()?;
        self.reap_fault(GRACEFUL_STOP_TIMEOUT)
    }

    /// Stop and synchronously reap this owned process. Idempotent after exit.
    ///
    /// SIGTERM first, SIGKILL only if the process will not leave. A node killed
    /// outright never closes its database. On the next launch, reth heals the
    /// torn static-file write by dropping the newest block. That strands the
    /// offchain-data projection one block short of the finalized head. Execution
    /// then waits forever for a projected parent that can never arrive. A stop
    /// that follows the operator path is what makes a restart reproducible.
    pub(crate) fn stop(&mut self) {
        self.stop_with_signal("TERM");
    }

    /// Explicit restart evidence must observe reaping, not rely on best-effort Drop.
    pub(crate) fn stop_and_reap(&mut self) -> Result<ExitStatus> {
        self.stop();
        self.exit_status()?
            .ok_or_else(|| eyre::eyre!("owned child {} remained live after stop", self.label))
    }

    /// Stop through the operator Ctrl-C path. Reth and the outer node launcher
    /// both observe SIGINT, allowing the launcher to drain its consensus thread
    /// before ExEx resources disappear.
    pub(crate) fn interrupt(&mut self) {
        self.stop_with_signal("INT");
    }

    fn stop_with_signal(&mut self, signal: &str) {
        if self.exited() {
            let _ = self.child.wait();
            return;
        }
        let _ = Command::new("kill")
            .args([format!("-{signal}"), self.child.id().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let deadline = std::time::Instant::now() + GRACEFUL_STOP_TIMEOUT;
        while std::time::Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                let _ = self.child.wait();
                return;
            }
            sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.stop();
    }
}

/// An owned docker container: `docker rm -f` on drop.
#[derive(Debug)]
pub(crate) struct DockerGuard {
    name: String,
    sudo: bool,
}

impl DockerGuard {
    pub(crate) fn new(name: impl Into<String>, sudo: bool) -> Self {
        Self {
            name: name.into(),
            sudo,
        }
    }
}

impl Drop for DockerGuard {
    fn drop(&mut self) {
        docker_rm(&self.name, self.sudo);
    }
}

/// A `Command` for `program`, `sudo`-wrapped when requested.
pub(crate) fn base_cmd(program: &str, sudo: bool) -> Command {
    if sudo {
        let mut c = Command::new("sudo");
        c.arg(program);
        c
    } else {
        Command::new(program)
    }
}

/// Best-effort `docker rm -f <name>`.
pub(crate) fn docker_rm(name: &str, sudo: bool) {
    let _ = base_cmd("docker", sudo)
        .args(["rm", "-f", name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Redirect a spawned node's stdout+stderr to `<node_dir>/node.log` (append),
/// with no stdin - the owned-process analogue of the shell `>> node.log 2>&1`.
pub(crate) fn attach_log(cmd: &mut Command, node_dir: &Path) -> Result<()> {
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(node_dir.join("node.log"))?;
    let log2 = log.try_clone()?;
    cmd.stdout(Stdio::from(log))
        .stderr(Stdio::from(log2))
        .stdin(Stdio::null());
    Ok(())
}

/// Whitespace-stripped file contents (`tr -d '[:space:]'`).
pub(crate) fn read_trimmed(path: &Path) -> Result<String> {
    Ok(fs::read_to_string(path)?
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect())
}

/// Normalize a hex secret file in place (strip whitespace/newlines). The caller
/// can then pass it to reth's file-based `--p2p-secret-key` flag, which parses
/// the file contents without trimming. The file stays where it is. Inlining the
/// hex into argv (`--p2p-secret-key-hex`) would expose the key to every local
/// user via `ps`.
pub(crate) fn normalized_secret_file(path: &Path) -> Result<PathBuf> {
    let trimmed = read_trimmed(path)?;
    fs::write(path, &trimmed)?;
    Ok(path.to_path_buf())
}

/// The `0x`-prefixed EVM key from `<vd>/evm-key.hex`.
pub(crate) fn read_evm_key(vd: &Path) -> Result<String> {
    let hex = read_trimmed(&vd.join("evm-key.hex"))?;
    Ok(if hex.starts_with("0x") {
        hex
    } else {
        format!("0x{hex}")
    })
}

/// 32 random bytes as hex (was `python3 secrets.token_hex(32)` / `openssl rand`).
pub(crate) fn random_hex_32() -> Result<String> {
    let mut buf = [0u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(hex::encode(buf))
}

/// The first run of `>= min_len` hex digits in `s` (keygen pubkey/signature).
pub(crate) fn first_hex(s: &str, min_len: usize) -> Option<String> {
    let mut cur = String::new();
    for c in s.chars() {
        if c.is_ascii_hexdigit() {
            cur.push(c);
        } else {
            if cur.len() >= min_len {
                return Some(cur);
            }
            cur.clear();
        }
    }
    (cur.len() >= min_len).then_some(cur)
}

/// Wait for a TCP listener on `127.0.0.1:port` (enclave socket readiness).
pub(crate) fn wait_tcp(port: u16, tries: u32) -> bool {
    let addr = format!("127.0.0.1:{port}");
    for _ in 0..tries {
        if TcpStream::connect(&addr).is_ok() {
            return true;
        }
        sleep(Duration::from_millis(100));
    }
    false
}

/// Run `program args...`, returning stdout on success or an error carrying stderr.
pub(crate) fn run_capture(program: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .wrap_err_with(|| format!("run {}", program.display()))?;
    if !out.status.success() {
        return Err(eyre!(
            "{} {:?} failed: {}",
            program.display(),
            args,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests;
